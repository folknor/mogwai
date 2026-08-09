// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Run configuration: the `mogwai.toml` schema, instrument-profile
//! construction/validation, and the sim-clock derivation that reads it. Split
//! out of `main.rs` because these are the load-time knobs the rest of the
//! server (the HTTP routes, the websocket replay) only ever reads back
//! through `AppState`/`SimClock`, never mutates.

use std::{collections::HashMap, path::PathBuf};

use anyhow::Context as _;

use mogwai_protocol::{InstrumentClass, InstrumentDef, MarketRegime, SimClock, WireAssetClass};
use rust_decimal::Decimal;

use crate::admission::AdmissionLimits;
use crate::source;

/// Replay/runtime configuration, loaded from a TOML config file at startup
/// (see `load`); never from ambient environment variables.
#[derive(Debug, Clone, serde::Deserialize)]
// `deny_unknown_fields` makes config.md's "a malformed file is a hard error"
// promise literally true for top-level knobs: a typo'd key (`warmup_n = 0`)
// used to fall through to the field default silently, so the operator ran with a
// value they never set (S20). `default` (missing keys keep built-in defaults) and
// `deny` (unknown keys are rejected) are orthogonal and compose. Each
// `[[instrument]]` table is guarded the same way, by `ConfiguredInstrument`.
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
    /// The default is a measured backstop against a wedged consumer, not a
    /// modeled pathology. Protocol 8's composition run raised it to 1,048,576:
    /// twice the worst measured p99.9 expansion of wall-second frame work over
    /// the prior 262,144, rounded up to a power of two. That holds 0.114 wall
    /// seconds at the worst measured rate, up from the 0.030 the prior value
    /// held, so the resize lengthens the horizon rather than shortening it.
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
    pub instrument: Option<ConfiguredInstrument>,
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
            account_id: DEFAULT_ACCOUNT_ID.to_owned(),
            oms_type: mogwai_protocol::OmsType::Netting,
            fill_band_vol_mult: 0.005,
            fill_band_max_ticks: 200,
            fill_sweep_interval_ms: 100,
            seed: None,
            // Honest-by-default: wall-clock pace the generator's inter-arrival
            // gaps so a no-config server serves a realistic live feed, matching
            // the committed mogwai.toml. 0.0 remains available as an explicit
            // firehose for fast local iteration. Until the coherent simulated
            // clock lands this is the 1x baseline; afterwards it is the 1x point
            // of the acceleration axis.
            speed: 1.0,
            server_heartbeat_ms: 0,
            // 24h: one day of warmup tape behind sim-now. A wrong horizon is made
            // loud (off-tape requests are refused, not silently under-served), so
            // the default's exactness is low-stakes.
            warmup_ns: 86_400_000_000_000,
            // The protocol-10 sizing, RETAINED at protocol 11 as a reviewed
            // policy exception. The standing resize formula proposed
            // 16,777,216 from the measured 1.014x frame-rate ratio, and the
            // proposal was rejected: unlike the pure refusal ceilings, the
            // fanout ring is EAGERLY ALLOCATED state proportional to this
            // depth, and the existing depth still holds about 0.466 wall
            // seconds at the protocol-11 worst measured p99.9 rate against
            // the 0.114s the protocol-10 resize was justified over. The
            // rejected capacity also deterministically breaks the
            // accept-before-fill invariant of
            // a_banded_limit_fills_from_the_run_sweep (5 of 5 against 5 of
            // 5 at this value) - an unresolved serving finding recorded in
            // notes/todo.md. A surge-exposed run should still size this
            // deliberately rather than inherit the default.
            fanout_depth: 4_194_304,
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
pub(crate) fn refuse_unfunded_settlement(
    cfg: &Config,
    defs: &[InstrumentDef],
) -> anyhow::Result<()> {
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
    pub fn load(path: Option<PathBuf>) -> anyhow::Result<Self> {
        let cfg: Self = match path {
            Some(path) => {
                let text = std::fs::read_to_string(path)?;
                let mut raw: toml::Table = toml::from_str(&text)?;
                if let Some(instrument) = raw
                    .get_mut("instrument")
                    .and_then(toml::Value::as_table_mut)
                {
                    *instrument = resolve_instrument_table(instrument.clone())?;
                }
                toml::Value::Table(raw).try_into()?
            }
            None => Self::default(),
        };
        // Validate here, not at the call site, so a validated `Config` is the
        // only kind `load` ever hands out - a future second consumer cannot
        // forget the check. (The clock and instrument validations stay with
        // their builders: they need boot-time inputs this loader lacks.)
        validate_balances(&cfg)?;
        validate_account_id(&cfg)?;
        validate_admission_limits(&cfg)?;
        validate_fill_band(&cfg)?;
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

fn preset_text(name: &str) -> Option<&'static str> {
    match name.to_ascii_uppercase().as_str() {
        "MNQ" => Some(include_str!("../presets/mnq.toml")),
        "MES" => Some(include_str!("../presets/mes.toml")),
        "BTCUSDT" => Some(include_str!("../presets/btcusdt.toml")),
        "ETHUSDT" => Some(include_str!("../presets/ethusdt.toml")),
        "SOLUSDT" => Some(include_str!("../presets/solusdt.toml")),
        _ => None,
    }
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

fn resolve_instrument_table(mut operator: toml::Table) -> anyhow::Result<toml::Table> {
    // The pre-class shape was seven flat fields. `deny_unknown_fields` would
    // refuse it anyway, but with a bare "unknown field `base`" that names
    // neither the replacement nor the shape it takes. Refuse it here instead,
    // where the message can say what to write.
    for removed in ["base", "quote"] {
        if operator.contains_key(removed) {
            anyhow::bail!(
                "instrument.{removed} was replaced by the [instrument.class] table; write \
                 kind = \"spot\" with base and quote under [instrument.class]"
            );
        }
    }
    let Some(name) = operator.remove("preset") else {
        return Ok(operator);
    };
    let name = name
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("instrument.preset must be a string"))?;
    let (mut merged, _) = effective_preset(name)?;
    let overrides = operator
        .remove("override")
        .and_then(|value| value.as_table().cloned())
        .unwrap_or_default();
    if !operator.is_empty() {
        let key = operator.keys().next().unwrap();
        anyhow::bail!("preset key {key} must be stated under instrument.override");
    }
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
        replace_dotted(&mut merged, &path, value)?;
    }
    Ok(merged)
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
    Future {
        underlying: String,
        settlement_currency: String,
        multiplier: Decimal,
        asset_class: WireAssetClass,
    },
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredMargin {
    pub(crate) initial_per_contract: Decimal,
    pub(crate) maintenance_per_contract: Decimal,
    #[serde(default)]
    pub(crate) breach_action: BreachAction,
}

#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BreachAction {
    #[default]
    Refuse,
    Liquidate,
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
            symbol: self.symbol.clone(),
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
            },
            price_precision: self.price_precision,
            size_precision: self.size_precision,
            price_increment: self.price_increment,
            size_increment: self.size_increment,
        }
    }
}

pub fn build_instrument_profiles(cfg: &Config) -> anyhow::Result<source::InstrumentProfiles> {
    let Some(configured) = &cfg.instrument else {
        return Ok(source::InstrumentProfiles::defaults());
    };
    let fp = mogwai_data::Fingerprint::from_repo_json();
    Ok(source::InstrumentProfiles::from_profiles(vec![
        profile_from_configured(configured, &fp)?,
    ]))
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
    scalars.symbol = def.symbol.clone();
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
        anyhow::anyhow!(
            "instrument {} generator.{} failed validation",
            def.symbol,
            err.field
        )
    })?;
    let size_grid = mogwai_data::SizeGrid::from_def(&def);
    scalars
        .validate_size_grid(size_grid.min_size)
        .map_err(|err| {
            anyhow::anyhow!(
                "instrument {} generator.{} failed size-grid validation",
                def.symbol,
                err.field
            )
        })?;
    for diagnostic in scalars.empirical_diagnostics(fp, size_grid.multiplier) {
        tracing::warn!(
            code = diagnostic.code,
            field = diagnostic.field,
            corpus = diagnostic.corpus,
            symbol = def.symbol,
            "generator scalar sits outside its empirical corpus range"
        );
    }
    for diagnostic in scalars.size_diagnostics(size_grid.min_size, size_grid.integral) {
        tracing::warn!(
            code = diagnostic.code,
            field = diagnostic.field,
            symbol = def.symbol,
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
    let fp = mogwai_data::Fingerprint::from_repo_json();
    profile_from_configured(&configured, &fp)
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

/// The run's clock. The epoch is fixed by config alone, while the wall anchor
/// decides only when a tick is delivered. `speed == 0.0` is unpaced delivery,
/// but the axis advances at wall rate for deadlines, sweeps and volatility.
pub(crate) fn build_run_clock(cfg: &Config, boot_wall_ns: u64) -> anyhow::Result<SimClock> {
    if !cfg.speed.is_finite() || cfg.speed < 0.0 {
        anyhow::bail!("speed must be finite and non-negative");
    }
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
    /// serving invariant - see notes/todo.md). A later mechanical
    /// application of a generated proposal must fail HERE and be argued,
    /// not slip through as bookkeeping.
    #[test]
    fn the_fanout_default_carries_the_protocol_11_exception() {
        assert_eq!(Config::default().fanout_depth, 4_194_304);
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
        let profile = source::InstrumentProfiles::defaults()
            .get("BTCUSDT")
            .expect("default profile")
            .clone();
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
        let err =
            refuse_unfunded_settlement(&cfg, &defs).expect_err("an unfunded quote refuses boot");
        assert!(err.to_string().contains("unfunded"), "{err}");

        refuse_unfunded_settlement(&Config::default(), &defs)
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
        refuse_unfunded_settlement(&cfg, &mogwai_protocol::default_instruments())
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
        let err = refuse_unfunded_settlement(&cfg, &[configured.def()])
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
        let table: toml::Table =
            toml::from_str("symbol = \"BTCUSDT\"\nbase = \"BTC\"\nquote = \"USDT\"\n").unwrap();
        let message = resolve_instrument_table(table)
            .expect_err("the removed flat class shape must refuse")
            .to_string();
        assert!(message.contains("instrument.class"), "{message}");
        assert!(message.contains("kind = \"spot\""), "{message}");
    }

    #[test]
    fn every_shipped_preset_parses_and_validates() {
        let fp = mogwai_data::Fingerprint::from_repo_json();
        for name in ["MNQ", "MES", "BTCUSDT", "ETHUSDT", "SOLUSDT"] {
            let (_, provenance) = effective_preset(name).unwrap();
            let table =
                toml::Table::from_iter([("preset".into(), toml::Value::String(name.into()))]);
            let resolved = resolve_instrument_table(table).unwrap();
            let configured: ConfiguredInstrument = toml::Value::Table(resolved).try_into().unwrap();
            validate_instrument_def(&configured.def()).unwrap();
            validate_instrument_options(&configured, &configured.def()).unwrap();
            let profile = profile_from_configured(&configured, &fp).unwrap();
            assert_preset_diagnostics(name, &profile, &fp, &provenance).unwrap();
        }
    }

    #[test]
    fn every_shipped_preset_quotes_a_positive_integral_width() {
        for name in ["MNQ", "MES", "BTCUSDT", "ETHUSDT", "SOLUSDT"] {
            let profile = profile_from_preset(name).unwrap();
            assert!(profile.scalars.quoted_width.ticks().get() > 0, "{name}");
        }
    }

    #[test]
    fn quote_seam_provenance_matches_the_protocol_10_landing() {
        // The placeholder era is over for the futures: MNQ's quote seams
        // are FITTED from the July TBBO month and MES inherits them loudly
        // as the standing stopgap; the crypto presets remain uncalibrated,
        // since no quote evidence covers spot. The fitted corpus strings
        // must name the MNQ evidence, so an MES reader can see the borrow.
        use mogwai_data::CalibrationProvenance;
        for name in ["BTCUSDT", "ETHUSDT", "SOLUSDT"] {
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
        }
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
        for name in ["MNQ", "MES", "BTCUSDT", "ETHUSDT", "SOLUSDT"] {
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
        for name in ["MNQ", "MES", "BTCUSDT", "ETHUSDT", "SOLUSDT"] {
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
    fn a_preset_key_restated_at_the_top_level_refuses_boot() {
        let table = toml::Table::from_iter([
            ("preset".into(), toml::Value::String("MNQ".into())),
            ("symbol".into(), toml::Value::String("MNQ".into())),
        ]);
        assert!(
            resolve_instrument_table(table)
                .unwrap_err()
                .to_string()
                .contains("override")
        );
    }

    #[test]
    fn an_override_of_a_path_the_preset_does_not_set_refuses_boot() {
        let table: toml::Table =
            toml::from_str("preset = \"MNQ\"\n[override]\n\"class.typo\" = \"x\"\n").unwrap();
        assert!(
            resolve_instrument_table(table)
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
            toml::Value::Table(resolve_instrument_table(table).unwrap())
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
        assert_eq!(mes.def.symbol, "MES");
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
            toml::Value::Table(resolve_instrument_table(table).unwrap())
                .try_into()
                .unwrap();
        let def = configured.def();
        assert_eq!(def.class.multiplier(), Decimal::from(2));
        assert_eq!(def.tick_value(), Decimal::new(50, 2));
    }
}
