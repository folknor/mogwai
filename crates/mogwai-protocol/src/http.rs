// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The venue's HTTP and upgrade grammars: every query string and JSON body a
//! consumer writes to a native route, and the `/health` and `/account` bodies it
//! reads back.
//!
//! These lived private to the venue crate, which left every writer outside it
//! spelling the keys by hand - the adapter with `format!`, a sister repository
//! by parsing the venue's source to learn the names. A renamed key compiled on
//! both sides and surfaced as a `400` at best. Owning the types here, beside
//! `routes`, makes the venue's decoder and every writer one definition, so a
//! moved name is a build failure wherever it is written.
//!
//! Every carrier here denies unknown fields, the requests and the two responses
//! alike. On a request, accepted-and-ignored is the worst reading: a
//! misspelled key is served as a wider or different request than the consumer
//! wrote, so it is refused with a `400` naming the key. On the response the
//! reasoning is the same from the other end. The venue, the adapter and every
//! Rust consumer build from one working tree, so there is no older reader for a
//! tolerance to protect, and a field the venue writes that a decoder silently
//! drops is exactly the gate that reads as green and checks nothing. A reader
//! that wants only a slice defines its own projection or reads a
//! `serde_json::Value`; the shared type stays strict.
//!
//! Account resolution, boat placement, divergence routing and every other
//! state-dependent decision stay in the venue. What lives here is grammar only.

use std::collections::HashMap;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::{OmsType, control::Divergence, risk::AccountPolicy};

/// Form-encode a query carrier. The carriers here are flat structs of strings,
/// integers, floats and options, which is the whole of what the encoder
/// supports, so a failure is a carrier gaining a field the form grammar cannot
/// hold - a defect in this module, not an input.
fn encode_query<T: Serialize>(query: &T) -> String {
    serde_urlencoded::to_string(query).expect("query carriers are flat and always form-encodable")
}

/// The `GET /health` body.
///
/// `status` and `fault` are one decision, computed together by the venue so
/// they cannot disagree: `Ok` while no boated river carries a tape fault,
/// `Faulted` the moment one does.
///
/// Field order is wire order, because `serde_json` writes fields as declared.
///
/// Denies unknown fields: a body carrying a field this build does not know is
/// a decode failure, not a newer venue to read around. Fields are not additive
/// for a typed reader. A reader that needs to survive a body it did not build
/// against reads it as an untyped `serde_json::Value` instead. The adapter's
/// identity probe is not one: it decodes this type whole, and a JSON body that is
/// not this shape is refused as someone else holding the address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Health {
    /// The one word a fleet poller gates on. It was the constant `ok` once,
    /// which made a status field that could not vary and scored a run with a
    /// stuck tape healthy for anything that did not go on to read `fault`.
    pub status: HealthStatus,
    pub oms_type: OmsType,
    /// Identifies this run, not this process.
    ///
    /// The endpoint is an ephemeral port, and a port outlives nothing: once a
    /// venue exits, the number is free and anything may take it. The venue also
    /// stops accepting before it exits, so the port is free while the process
    /// is still alive. The seed is unique per run and already reported in the
    /// readiness record, so a launcher can hand it to its consumers and they can
    /// check they are still talking to the venue they were given.
    pub run_seed: u64,
    /// A faulted tape on any boated river, not merely on the boot one.
    ///
    /// A latched materialization fault is reported first, with kind
    /// `materialize` and clock zero, whether or not any boat carries a fault: it
    /// happened before there was a boat to carry it, so reading only boats would
    /// report a venue that cannot produce the water it promised as healthy. The
    /// boat selection below is the fallback when there is none.
    ///
    /// Under the open instrument set a run places a boat per keyed river and
    /// each can fault independently, so one optional object over N boats means
    /// choosing which answers: the faulted river with the smallest symbol. That
    /// keeps the field deterministic across polls and still answers the
    /// question a poller has - is any river faulted - because one faulted river
    /// already condemns the run. A second simultaneous fault is not reported.
    ///
    /// Always serialized, as `null` when healthy.
    ///
    /// Separate from the venue's terminal fault shutdown path: this is what a
    /// poller can see before a run dies, not when it dies.
    pub fault: Option<HealthFault>,
    /// The two account-lifecycle boot constants, published for the attaching
    /// consumer the readiness record cannot reach: a posted ledger's survival
    /// depends on both (a reset-enabled venue discards it at the first socket;
    /// a nonzero TTL can collect a posted, never-connected account before the
    /// socket seats). Same names and meanings as the `ReadyRecord` fields -
    /// `false` means reconnection preserves the ledger. Always present: a
    /// consumer gates on these, and an absent field must not read as a third
    /// state.
    pub reset_account_on_reconnect: bool,
    /// Milliseconds a frozen account may sit unconnected before the reaper
    /// collects it; `0` means never, which is the default.
    pub account_ttl_ms: u64,
}

/// The `/health` status word.
///
/// A closed pair on purpose: it is derived from whether `fault` is present, so
/// there is no third state for it to grow. The extensible vocabulary is
/// [`HealthFault::kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Ok,
    Faulted,
}

impl HealthStatus {
    /// The status a run carrying, or not carrying, a reported fault has.
    #[must_use]
    pub fn from_faulted(faulted: bool) -> Self {
        if faulted { Self::Faulted } else { Self::Ok }
    }
}

/// The tape fault `/health` reports. Denies unknown fields, like [`Health`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthFault {
    /// The river that faulted.
    pub symbol: String,
    /// The fault taxonomy name, shared verbatim with the venue's exit line.
    ///
    /// A string rather than an enum because the taxonomy is open: it grows
    /// with every new tick-fault shape, and a kind is a value naming what went
    /// wrong rather than a key in the body's shape. Strictness governs which
    /// fields the body has; it has nothing to say about which kinds exist, so
    /// an unfamiliar kind decodes.
    pub kind: String,
    /// The source cursor the fault is dated by. Zero for a fault no source's
    /// cursor dates, such as an injected one; `kind` says which.
    pub clock_ns: u64,
}

/// The `GET /account` query string.
///
/// Denies unknown fields because accepted-and-ignored is the worst reading
/// available here: a consumer that misspells the one key it can send is
/// otherwise handed the default account's snapshot under the name of the
/// account it asked about, and nothing in the answer says which ledger it
/// describes. The price is that any unrecognized key is a `400`, a future key
/// included; relaxing it is a wire change that owes its own reasoning.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountQuery {
    /// The ledger to read. Absent means the venue's default account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

impl AccountQuery {
    /// The form-encoded query string, without the leading `?`.
    #[must_use]
    pub fn to_query(&self) -> String {
        encode_query(self)
    }
}

/// The `GET /account` body: one account's ledger, the axis its stamp lives on,
/// and the sweeper's progress on the seats its passengers hold.
///
/// The account is nested under `account` rather than flattened beside the two
/// response-only keys, and that is what lets the body be strict. `serde(flatten)`
/// cannot combine with `deny_unknown_fields`, so a flattened body either
/// tolerated stray keys or made every decoder take `clock` and `sweep_passes`
/// off by name before decoding the rest - a hand-kept list of keys beside the
/// type, which is the pattern a shared carrier exists to remove. Nested, the
/// venue writes this type and every reader decodes it, and a moved name is a
/// build failure on both sides.
///
/// Field order is wire order, because `serde_json` writes fields as declared.
///
/// Risk rides inside the account as [`crate::AccountState::risk`], always
/// `Some` on this body, rather than as a sibling here. `AccountState` is also
/// the pushed frame's payload, where `None` is meaningful (an unpoliced
/// account's frame reports no budget), so the field stays optional on the type;
/// a required sibling on this body would leave the nested account carrying a
/// second `risk` slot that must always be absent, two spellings of one fact
/// kept apart by convention. The venue always sets it here because an
/// evaluator wants an unpoliced account's equity too.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountSnapshot {
    /// Always [`ClockAxis::Venue`] today. Present so a consumer can never
    /// mistake the account's `ts_event` for boat time.
    ///
    /// Stamped on the venue clock, deliberately. A ledger spans every river
    /// its account's passengers have boarded, so there is no boat axis to put
    /// it on: stamp from one boat and a push from a later-placed boat on
    /// another river is ahead of the pull; stamp from the newest and it is
    /// behind. A consumer orders pulls against pushes by sequence.
    pub clock: ClockAxis,
    /// The ledger itself, the same type the pushed `AccountState` frame
    /// carries.
    pub account: crate::AccountState,
    /// The fill sweeper's completed-pass count on each boat this account is
    /// seated on, sorted by symbol.
    ///
    /// Account-scoped, and that placement is the whole of it. The count is the
    /// only observable that says the engine work behind a fill, a settlement or
    /// a funding charge has actually run, so something had to carry it - and it
    /// first landed on `/health`, which enumerated every boat in the run to any
    /// caller. That is the anonymous boat-discovery surface `/clock` was cut
    /// back to remove: symbols and cadences are what other accounts asked for,
    /// and passengers of different accounts are owed invisibility. Here the
    /// caller must name an account to be told anything, on the same footing as
    /// the balances and risk state beside it, and it is told only about seats
    /// its own passengers boarded. An account seated nowhere - unopened, or
    /// frozen with its last passenger gone - gets an empty list, which is the
    /// truth rather than a redaction: an unseated account's rivers are not
    /// swept.
    ///
    /// Keyed by symbol alone, with no cadence field, because one ledger carries
    /// one cadence per river - a second is refused at admission - so the symbol
    /// already names the seat unambiguously and publishing the speed would add
    /// an observable for nothing.
    pub sweep_passes: Vec<SweepPasses>,
}

/// One seat's completed-pass count on [`AccountSnapshot::sweep_passes`].
/// Monotonic within a boat's life and observation only: nothing in scheduling,
/// pacing or the engine reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepPasses {
    pub symbol: String,
    pub completed: u64,
}

/// Which axis a timestamp lives on. The sibling of `VenueClock::boat_clock`,
/// and the reason a venue stamp is honest rather than a look-ahead in disguise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClockAxis {
    Venue,
}

/// The `/operator/trades` and `/operator/quotes` query string.
///
/// Denies unknown fields because every key here bounds the window, so a
/// misspelled one is served as a wider request than the consumer wrote:
/// `?limti=5` reads as the default limit, `?strat=` as history from the origin,
/// and the answer looks like a perfectly good page. The retired `regime` key is
/// the worked example - it became boot config for the whole run, so a consumer
/// still sending it was being answered on a realization it did not ask for.
/// Refusing makes that a `400` naming the key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryQuery {
    pub symbol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

impl HistoryQuery {
    /// The form-encoded query string, without the leading `?`. An unset bound
    /// is omitted rather than sent empty, which the venue would refuse.
    #[must_use]
    pub fn to_query(&self) -> String {
        encode_query(self)
    }
}

/// The `POST /accounts` body: open an account on terms the consumer states,
/// before it trades.
///
/// Structured account config goes over HTTP for the same reason a divergence
/// does: it is a nested document validated at its own boundary, and the socket
/// query string carries scalars. A socket then names the account it opened with
/// `?account=`, and only that id crosses the upgrade.
///
/// Optional, and that is the design rather than a convenience. Account
/// resolution is total: a socket naming no account is served under the default
/// one, and a socket naming an account nobody posted gets that account opened on
/// the run's terms. What it buys is the case the default cannot express - a
/// batch of subagents on one exchange, each sized differently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAccountRequest {
    pub account_id: String,
    /// Opening balances by currency. The venue's `[balances]` is what an
    /// unnamed account gets; this is the same value stated per account.
    ///
    /// String-spelled, like every other money quantity that crosses into the
    /// venue: `{"USDT":"250000"}`, never `{"USDT":250000}`. A bare JSON number
    /// goes through `f64`, so a wide opening balance would be silently rounded
    /// and `1e-30` would fund the account with zero. The policy fields stay
    /// number-tolerant on purpose: they are thresholds and fractions that are
    /// also spelled in TOML.
    #[serde(with = "crate::decimal::str_map", default)]
    pub balances: HashMap<String, Decimal>,
    /// The rules the venue enforces against this account, stated inline.
    /// Absent means unpoliced unless `policy_preset` names one.
    ///
    /// Omitted on encode when it equals `AccountPolicy::default()`, which the
    /// venue decodes an absent policy to - so absent and default are one state
    /// on the wire, and a typed writer sends exactly what a hand-written body
    /// that stated no policy did.
    #[serde(default, skip_serializing_if = "is_default_policy")]
    pub policy: AccountPolicy,
    /// A registered or shipped policy to use instead of restating one.
    ///
    /// Resolution is total and three-step, the same shape a symbol resolves in:
    /// inline knobs win, else this name, else unpoliced. A name nobody has is an
    /// error rather than a silent fall to unpoliced, because a run that believes
    /// it is enforced and is not is the worst of the three outcomes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_preset: Option<String>,
}

fn is_default_policy(policy: &AccountPolicy) -> bool {
    *policy == AccountPolicy::default()
}

/// The `/ws` upgrade query string.
///
/// Denies unknown fields as a wire-compatibility decision, taken knowingly: a
/// consumer that sends a key this carrier does not handle is refused rather than
/// silently served a different river, speed or duration than it asked for. The
/// price is that any unrecognized key is a `400`, including one an unrelated
/// consumer, proxy or tracing layer appends, and including a future key added
/// before its handling lands. Relaxing it later is a wire change that owes its
/// own reasoning, not a tidy-up.
///
/// A repeated key is refused as a duplicate field, like an unknown one: the
/// derived struct decoder sees both occurrences, so `symbol=MNQ&symbol=MES` is a
/// `400` rather than a guess at which river was meant.
///
/// The identity key was `session` until the callsign ruling retired `session`
/// as a name for anything but the trading day. Denying unknown fields is what
/// makes that break loud for a consumer still sending the old spelling.
///
/// Field order is encode order: `account` leads because every writer names one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SocketQuery {
    /// The account to trade under. Absent means the venue's default account,
    /// which exists for the ephemeral single-consumer venue where naming one
    /// would be ceremony - it is not a venue-wide account every connection
    /// shares.
    ///
    /// The id is the consumer's and outlives the connection, so presenting the
    /// same one again resumes that ledger. The venue cannot distinguish a
    /// reconnect from a stranger claiming the id and does not try; anyone who
    /// knows an id can claim its account, which is acceptable on a loopback
    /// venue serving one orchestrator's subagents and is stated rather than
    /// assumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// Absent means "the run's boot symbol", which is what every consumer that
    /// predates this carrier sends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// Absent means the venue's configured `speed`. Finite and non-negative,
    /// quantized to micro-multiples in the sharing key, so `100` and
    /// `100.0000001` board the same boat. An unserved speed places a second
    /// boat on the same water rather than being refused - speed mutates no
    /// generated value, so it is a second cursor, not a second river. The one
    /// refusal left is per ledger: an account already riding this river at
    /// another speed would be judged on two clocks.
    ///
    /// Open-ended sharing applies only to the unnamed form, the request that
    /// says "wherever you are is fine". A named window gets a private placement
    /// for its account and callsign even when another account already requested
    /// identical bounds. Both placements read the same deterministic river from
    /// the named start; the window changes cursor ownership, not water identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
    /// Absent means indefinite. Simulated milliseconds, measured on the boat's
    /// clock from this passenger's boarding instant and not from boot. A
    /// duration is a property of the passenger, so passengers with different
    /// durations still share one boat; each announces
    /// `PassengerDurationComplete` and closes at its own deadline, and the boat
    /// winds down when the last one leaves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Inclusive start of a named tape window. It is valid only with
    /// `window_end_ns`, and the pair is mutually exclusive with `duration_ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_start_ns: Option<u64>,
    /// Exclusive completion boundary of a named tape window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_end_ns: Option<u64>,
    /// The generator arm this passenger's water carries, in four flat keys so
    /// the query string stays readable and unknown-field refusal still covers
    /// them.
    ///
    /// This is the fork. A passenger carrying an arm boards a different river
    /// than one without it, rather than mutating water someone else may already
    /// be reading, so two accounts can run a clean strategy and a surged one on
    /// one exchange without either seeing the other's weather. It rides the
    /// upgrade rather than a control post because a posted default is run-wide
    /// state: on a shared venue that would let one consumer decide what every
    /// other account's next boarding resolves to.
    ///
    /// `surge_start_ms` is an offset from the run origin, not from this
    /// passenger's boarding instant. That is what lets two passengers share:
    /// "starting when I connect" names a different window for every boarding
    /// instant, so it would fork a river per connection and share nothing. The
    /// consequence to expect is that boarding late with a zero offset boards
    /// water whose surge is already over - the river had its weather whether or
    /// not anyone was aboard, which is what exogenous water means.
    ///
    /// Milliseconds, deliberately, where the identity underneath is
    /// nanoseconds. Two harness paths computing the same intended start through
    /// different units would otherwise differ by sub-millisecond residue and
    /// each strand a river of its own against a cap that never evicts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surge_start_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surge_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surge_rate_mult: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surge_children_mult: Option<f64>,
    /// The identity this socket presents, so several sockets presenting the
    /// same value can coexist on one ledger.
    ///
    /// A nautilus host dials `/ws` twice - market data and execution - and both
    /// legs carry the same `account` by construction, so without this the second
    /// dial evicts the first and the host disconnects itself. Sockets sharing a
    /// callsign coexist; a socket presenting a different one, or none, takes the
    /// ledger. Absent on both sides is therefore exactly the pre-callsign
    /// behaviour.
    ///
    /// The venue reads nothing into the string beyond equality: it is stable
    /// across related sockets and their redials, and fresh in a restarted
    /// process. Like the account id it is a bearer token - anyone who knows the
    /// pair can join that ledger rather than displace it - which is acceptable on
    /// a loopback venue and is stated rather than assumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,
}

impl SocketQuery {
    /// The form-encoded query string, without the leading `?`.
    #[must_use]
    pub fn to_query(&self) -> String {
        encode_query(self)
    }
}

/// The `POST /control/divergence` body.
///
/// A flat envelope - `kind`, `args`, and the scope keys beside them - rather
/// than the tagged [`Divergence`] itself, so the scope keys cannot collide with
/// a variant's own argument names. [`DivergenceRequest::new`] builds it from a
/// typed divergence and [`DivergenceRequest::divergence`] recovers one, so no
/// writer strips the tag by hand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DivergenceRequest {
    /// Checked against the order's own symbol by `CancelOpenOrderSilently`,
    /// which refuses a mismatch rather than preferring either.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// Which account the divergence applies to. Absent means every account,
    /// which is what an operator on a single-account venue wants and what every
    /// existing scenario file already writes. See
    /// [`Divergence::accepts_account_scope`] for which kinds honour it; the one
    /// that does not refuses it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub kind: String,
    #[serde(default)]
    pub args: serde_json::Map<String, serde_json::Value>,
}

impl DivergenceRequest {
    /// The envelope for a typed divergence under an optional account scope.
    ///
    /// The account is carried exactly as given, never dropped for a kind that
    /// cannot honour it: the venue refuses that pairing, and discarding the
    /// scope here would silently widen a request aimed at one ledger to the
    /// whole venue.
    #[must_use]
    pub fn new(divergence: &Divergence, account: Option<String>) -> Self {
        let serde_json::Value::Object(mut args) =
            serde_json::to_value(divergence).expect("a Divergence always serializes")
        else {
            unreachable!("Divergence is internally tagged, so it serializes as an object")
        };
        let Some(serde_json::Value::String(kind)) = args.remove("type") else {
            unreachable!("Divergence serialization carries its string tag")
        };
        Self {
            symbol: None,
            account,
            kind,
            args,
        }
    }

    /// The typed divergence this envelope names, refusing an unknown kind or an
    /// argument that kind does not take.
    ///
    /// An unknown kind is refused against `DIVERGENCE_KINDS`, which is proven
    /// complete against the enum. An unknown argument is refused by the enum's
    /// own decode, which denies unknown fields per variant, so there is no second
    /// list of argument names here to fall out of step with the variants. A
    /// `type` key inside `args` is refused explicitly, since the outer `kind`
    /// would otherwise silently replace it.
    ///
    /// # Errors
    /// A message naming the unknown kind, the unknown argument, or the decode
    /// failure.
    pub fn divergence(&self) -> Result<Divergence, String> {
        if !crate::control::DIVERGENCE_KINDS.contains(&self.kind.as_str()) {
            return Err(format!("unknown divergence kind {}", self.kind));
        }
        if self.args.contains_key("type") {
            return Err(format!(
                "unknown field args.type for divergence kind {}",
                self.kind
            ));
        }
        let mut value = self.args.clone();
        value.insert(
            "type".to_owned(),
            serde_json::Value::String(self.kind.clone()),
        );
        serde_json::from_value(serde_json::Value::Object(value))
            .map_err(|err| format!("invalid args for divergence kind {}: {err}", self.kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health(fault: Option<HealthFault>) -> Health {
        Health {
            status: HealthStatus::from_faulted(fault.is_some()),
            oms_type: OmsType::Netting,
            run_seed: 7,
            fault,
            reset_account_on_reconnect: false,
            account_ttl_ms: 0,
        }
    }

    /// The exact bytes a healthy and a faulted venue write. `fault` is `null`
    /// rather than absent when healthy, and the field order is the wire order a
    /// reader of the raw body has always seen.
    #[test]
    fn health_writes_the_wire_bytes_consumers_read() {
        assert_eq!(
            serde_json::to_string(&health(None)).unwrap(),
            r#"{"status":"ok","oms_type":"netting","run_seed":7,"fault":null,"reset_account_on_reconnect":false,"account_ttl_ms":0}"#
        );
        let faulted = health(Some(HealthFault {
            symbol: "MNQ".to_owned(),
            kind: "arrival.intensity_ceiling".to_owned(),
            clock_ns: 9,
        }));
        assert_eq!(
            serde_json::to_string(&faulted).unwrap(),
            r#"{"status":"faulted","oms_type":"netting","run_seed":7,"fault":{"symbol":"MNQ","kind":"arrival.intensity_ceiling","clock_ns":9},"reset_account_on_reconnect":false,"account_ttl_ms":0}"#
        );
        let decoded: Health = serde_json::from_str(&serde_json::to_string(&faulted).unwrap())
            .expect("a written health body decodes");
        assert_eq!(decoded, faulted);
    }

    /// The body's shape is exact in both directions: an unknown field is
    /// refused on `Health` and on the nested `HealthFault`, and a missing fact
    /// is refused rather than defaulted. The fault kind is a value in an open
    /// taxonomy, so a kind this build has never named still decodes.
    #[test]
    fn health_refuses_unknown_and_missing_fields_but_not_unknown_kinds() {
        let exact = r#"{"status":"faulted","oms_type":"netting","run_seed":7,"fault":{"symbol":"MNQ","kind":"some.future_fault","clock_ns":0},"reset_account_on_reconnect":true,"account_ttl_ms":5}"#;
        let decoded: Health = serde_json::from_str(exact).expect("the exact body decodes");
        assert_eq!(decoded.fault.unwrap().kind, "some.future_fault");

        let top = exact.replacen(
            r#""account_ttl_ms":5"#,
            r#""account_ttl_ms":5,"added_later":1"#,
            1,
        );
        let err = serde_json::from_str::<Health>(&top).expect_err("an unknown field is refused");
        assert!(err.to_string().contains("added_later"), "{err}");

        let nested = exact.replacen(r#""clock_ns":0"#, r#""clock_ns":0,"added_later":1"#, 1);
        let err =
            serde_json::from_str::<Health>(&nested).expect_err("an unknown fault field is refused");
        assert!(err.to_string().contains("added_later"), "{err}");

        let missing =
            r#"{"status":"ok","oms_type":"netting","run_seed":7,"fault":null,"account_ttl_ms":0}"#;
        assert!(
            serde_json::from_str::<Health>(missing).is_err(),
            "reset_account_on_reconnect was defaulted instead of required"
        );
    }

    /// The exact `GET /account` body the venue writes: the account nested under
    /// `account`, between `clock` and `sweep_passes`, with its risk block inside
    /// it. Decoding and re-encoding reproduces the bytes, so no field is dropped
    /// or reordered on either side.
    const ACCOUNT_SNAPSHOT: &str = r#"{"clock":"venue","account":{"account_id":"WYRD-01","balances":[{"currency":"USD","total":"10000","free":"10000","locked":"0"}],"positions":[],"risk":{"equity":"10000","peak_equity":"10000","day_open_equity":"10000"},"ts_event":7},"sweep_passes":[{"symbol":"MNQ","completed":3}]}"#;

    #[test]
    fn account_snapshot_writes_the_wire_bytes_consumers_read() {
        let decoded: AccountSnapshot =
            serde_json::from_str(ACCOUNT_SNAPSHOT).expect("the venue's body decodes");
        assert_eq!(decoded.clock, ClockAxis::Venue);
        assert_eq!(decoded.account.account_id.as_str(), "WYRD-01");
        assert_eq!(decoded.account.ts_event, 7);
        assert!(decoded.account.risk.is_some(), "risk rides the account");
        assert_eq!(
            decoded.sweep_passes,
            vec![SweepPasses {
                symbol: "MNQ".to_owned(),
                completed: 3,
            }]
        );
        assert_eq!(serde_json::to_string(&decoded).unwrap(), ACCOUNT_SNAPSHOT);
    }

    /// Strict at both levels: a key the snapshot does not know is refused, and
    /// so is one inside the nested account. The flattened shape could not do
    /// the first without a decoder-side list of keys, which is what nesting
    /// removed. A flat body - the account's keys at the top level - is refused
    /// too, so a double serving the retired shape cannot pass as the venue.
    #[test]
    fn account_snapshot_refuses_unknown_keys_at_both_levels() {
        let top = ACCOUNT_SNAPSHOT.replacen(
            r#""clock":"venue","#,
            r#""clock":"venue","added_later":1,"#,
            1,
        );
        let err = serde_json::from_str::<AccountSnapshot>(&top)
            .expect_err("an unknown snapshot key is refused");
        assert!(err.to_string().contains("added_later"), "{err}");

        let nested =
            ACCOUNT_SNAPSHOT.replacen(r#""ts_event":7"#, r#""ts_event":7,"added_later":1"#, 1);
        let err = serde_json::from_str::<AccountSnapshot>(&nested)
            .expect_err("an unknown account key is refused");
        assert!(err.to_string().contains("added_later"), "{err}");

        let flat = r#"{"clock":"venue","account_id":"WYRD-01","balances":[],"positions":[],"ts_event":7,"sweep_passes":[]}"#;
        assert!(
            serde_json::from_str::<AccountSnapshot>(flat).is_err(),
            "the retired flattened body decoded"
        );
    }

    #[test]
    fn history_query_omits_unset_bounds_and_refuses_unknown_keys() {
        let bare = HistoryQuery {
            symbol: "MNQ".to_owned(),
            start: None,
            end: None,
            limit: None,
        };
        assert_eq!(bare.to_query(), "symbol=MNQ");
        let bounded = HistoryQuery {
            start: Some(7),
            end: Some(9),
            limit: Some(5),
            ..bare
        };
        assert_eq!(bounded.to_query(), "symbol=MNQ&start=7&end=9&limit=5");
        assert_eq!(
            serde_urlencoded::from_str::<HistoryQuery>(&bounded.to_query()).unwrap(),
            bounded
        );
        let refusal = serde_urlencoded::from_str::<HistoryQuery>("symbol=MNQ&limti=5")
            .expect_err("a misspelled bound must be refused");
        assert!(refusal.to_string().contains("limti"), "{refusal}");
    }

    #[test]
    fn socket_and_account_queries_round_trip_through_the_venue_decoder() {
        let socket = SocketQuery {
            account: Some("WYRD-01".to_owned()),
            symbol: Some("MNQ".to_owned()),
            speed: Some(12.5),
            window_start_ns: Some(1_400),
            window_end_ns: Some(2_000),
            callsign: Some("mogwai-1-2".to_owned()),
            ..SocketQuery::default()
        };
        let encoded = socket.to_query();
        assert_eq!(
            encoded,
            "account=WYRD-01&symbol=MNQ&speed=12.5&window_start_ns=1400&window_end_ns=2000&callsign=mogwai-1-2"
        );
        assert_eq!(
            serde_urlencoded::from_str::<SocketQuery>(&encoded).unwrap(),
            socket
        );
        assert!(serde_urlencoded::from_str::<SocketQuery>("session=x").is_err());
        let duplicate = serde_urlencoded::from_str::<SocketQuery>("symbol=MNQ&symbol=MES")
            .expect_err("a repeated key must be refused, not resolved");
        assert!(duplicate.to_string().contains("duplicate"), "{duplicate}");

        let account = AccountQuery {
            account: Some("ISSUER:7".to_owned()),
        };
        assert_eq!(
            serde_urlencoded::from_str::<AccountQuery>(&account.to_query()).unwrap(),
            account
        );
        assert_eq!(AccountQuery::default().to_query(), "");
    }

    /// An unstated policy writes no `policy` key, balances go out as strings,
    /// and a numeric balance is still refused on decode.
    #[test]
    fn open_account_request_writes_string_balances_and_omits_the_default_policy() {
        let request = OpenAccountRequest {
            account_id: "WYRD-01".to_owned(),
            balances: HashMap::from([("USD".to_owned(), Decimal::from(25_000))]),
            policy: AccountPolicy::default(),
            policy_preset: Some("eod-trail".to_owned()),
        };
        let body = serde_json::to_string(&request).unwrap();
        assert_eq!(
            body,
            r#"{"account_id":"WYRD-01","balances":{"USD":"25000"},"policy_preset":"eod-trail"}"#
        );
        assert_eq!(
            serde_json::from_str::<OpenAccountRequest>(&body).unwrap(),
            request
        );
        assert!(
            serde_json::from_str::<OpenAccountRequest>(
                r#"{"account_id":"WYRD-01","balances":{"USD":25000}}"#
            )
            .is_err()
        );
    }

    /// One instance of every kind, with every optional field set away from its
    /// default so a dropped field shows as an inequality.
    fn every_kind() -> [Divergence; 12] {
        [
            Divergence::PartialFillNext {
                client_order_id: "O-1".to_owned(),
                fraction: Decimal::new(5, 1),
            },
            Divergence::RejectNextSubmit {
                reason: "no".to_owned(),
            },
            Divergence::RejectNextCancel {
                reason: "no".to_owned(),
            },
            Divergence::DelayAcks { ms: 3 },
            Divergence::CommandLatency {
                submit_act_ms: 1,
                modify_act_ms: 2,
                cancel_act_ms: 3,
                submit_ack_ms: 4,
                modify_ack_ms: 5,
                cancel_ack_ms: 6,
            },
            Divergence::DuplicateNextFill {},
            Divergence::DropNextAccountUpdate {},
            Divergence::GoDark { ms: 4 },
            Divergence::StallData { ms: 5 },
            Divergence::FeeSurcharge {
                mult: Decimal::from(2),
                window_ms: 6,
            },
            Divergence::CancelOpenOrderSilently {
                client_order_id: "O-2".to_owned(),
            },
            Divergence::FaultTape {},
        ]
    }

    /// Every kind survives the envelope, and the scope is carried as given even
    /// for the kind that refuses one.
    #[test]
    fn divergence_request_round_trips_every_kind_and_keeps_the_scope() {
        let kinds = every_kind();
        assert_eq!(kinds.len(), crate::control::DIVERGENCE_KINDS.len());
        for divergence in kinds {
            let request = DivergenceRequest::new(&divergence, Some("WYRD-01".to_owned()));
            assert_eq!(request.account.as_deref(), Some("WYRD-01"));
            let wire: DivergenceRequest =
                serde_json::from_str(&serde_json::to_string(&request).unwrap()).unwrap();
            assert_eq!(wire.divergence().unwrap(), divergence);
        }
        let typo = DivergenceRequest {
            symbol: None,
            account: None,
            kind: "DelayAcks".to_owned(),
            args: serde_json::from_str(r#"{"msec":3}"#).unwrap(),
        };
        assert!(typo.divergence().unwrap_err().contains("msec"));
    }

    /// The argument list is the variants' own fields, with no second list to
    /// drift: every kind, unit kinds included, refuses a key it does not take,
    /// both through the control envelope and decoded directly as a havoc
    /// config does. A `type` smuggled into `args` cannot rename the kind, and a
    /// kind nobody has is refused by name.
    #[test]
    fn every_kind_refuses_an_argument_it_does_not_take() {
        for divergence in every_kind() {
            let mut request = DivergenceRequest::new(&divergence, None);
            request
                .args
                .insert("not_a_knob".to_owned(), serde_json::Value::from(1));
            let refusal = request
                .divergence()
                .expect_err("an unknown argument must be refused");
            assert!(
                refusal.contains("not_a_knob"),
                "{} refused for another reason: {refusal}",
                request.kind
            );

            let mut direct = serde_json::to_value(&divergence).unwrap();
            direct["not_a_knob"] = serde_json::Value::from(1);
            assert!(
                serde_json::from_value::<Divergence>(direct).is_err(),
                "{} decoded directly with an unknown key",
                request.kind
            );

            let mut retagged = DivergenceRequest::new(&divergence, None);
            retagged
                .args
                .insert("type".to_owned(), serde_json::Value::from("FaultTape"));
            assert!(retagged.divergence().is_err());
        }
        let unknown = DivergenceRequest {
            symbol: None,
            account: None,
            kind: "Earthquake".to_owned(),
            args: serde_json::Map::new(),
        };
        assert_eq!(
            unknown.divergence().unwrap_err(),
            "unknown divergence kind Earthquake"
        );
    }
}
